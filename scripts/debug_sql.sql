/* SELECT file_name, file_device, COUNT(*) */
/* FROM bep_block_info */
/* WHERE TRUE */
/* GROUP BY file_device, file_name */

/* SELECT f.*, bi.* */
/* FROM bep_file_info f */
/*     JOIN bep_block_info bi ON f.name = bi.file_name AND f.folder = bi.file_folder AND f.device = bi.file_device */
/* WHERE f.folder = 'orig_dir' AND f.device = 'RFJWU2I-J6ULCCU-36JJOGP-YWEBZAB-T5KHSUO-ROXYHCJ-CUSRTBY-WKZHNQY' */

/* SELECT * */
/* FROM bep_file_info f */
/*     JOIN bep_file_version v ON f.name = v.file_name AND f.folder = v.file_folder AND f.device = v.file_device */
/* WHERE f.folder = 'orig_dir' AND f.device = 'RFJWU2I-J6ULCCU-36JJOGP-YWEBZAB-T5KHSUO-ROXYHCJ-CUSRTBY-WKZHNQY' */

-- -- query to find the files that haven't been stored locally yet
-- SELECT fin.device, fin.folder, fin.name
-- FROM bep_file_location flo RIGHT JOIN bep_file_info fin ON
--  flo.loc_folder = fin.folder AND flo.loc_device = fin.device AND flo.loc_name = fin.name
-- GROUP BY fin.device, fin.folder, fin.name
-- HAVING count(flo.loc_name)
-- ;

SELECT *
FROM bep_file_location flo RIGHT JOIN bep_file_info fin ON
 flo.loc_folder = fin.folder AND flo.loc_device = fin.device AND flo.loc_name = fin.name
WHERE fin.device = 'DC3KS6Y-XZWHD4F-T2L5QY6-LJ2GR7X-THN23CX-33LLN46-WZ7FAQO-AVE3BQV'
;
